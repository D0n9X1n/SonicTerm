use super::*;
use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use std::path::PathBuf;

fn bundled_font_handle() -> FontDataHandle {
    FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: FontOrigin::BuiltIn,
        coverage: None,
    }
}

#[test]
fn missing_fallback_replaces_a_nonzero_multibyte_cluster_without_panicking() {
    // A stale fallback used to apply the source range 1..8 to a three-byte replacement string.
    let base = ParsedFont::from_locator(&bundled_font_handle()).unwrap();
    let mut missing_fallback = base.clone();
    missing_fallback.handle.source = FontDataSource::OnDisk(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/shaper/sonicterm-missing-fallback-font.ttf"),
    );
    let config = config::ConfigHandle::new(config::Config::default());
    let shaper = HarfbuzzShaper::new(&config, &[base, missing_fallback]).unwrap();
    let text = "a\u{1f600}\u{fe0e}";
    let mut no_glyphs = Vec::new();

    let glyphs = shaper
        .shape(text, 12.0, 96, &mut no_glyphs, None, Direction::LeftToRight, None, None)
        .unwrap();

    let replacements: Vec<_> = glyphs.iter().filter(|glyph| glyph.cluster == 1).collect();
    assert!(!replacements.is_empty(), "missing fallback should preserve the source cluster");
    assert!(replacements
        .iter()
        .any(|glyph| { matches!(glyph.only_char, Some(std::char::REPLACEMENT_CHARACTER | '?')) }));
    assert_eq!(
        replacements.iter().map(|glyph| u16::from(glyph.num_cells)).sum::<u16>(),
        UnicodeWidthStr::width("\u{1f600}\u{fe0e}") as u16
    );
}
