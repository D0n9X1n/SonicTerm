//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::color::{linear_u8_to_srgb8, SrgbaPixel};
use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use crate::parser::ParsedFont;
use crate::rangeset::RangeSet;
use crate::select_fallback_fonts;
use std::path::PathBuf;

fn fallback_fixture(coverage_chars: &[char], is_math_font: bool) -> ParsedFont {
    let mut coverage = RangeSet::new();
    for ch in coverage_chars {
        coverage.add(*ch as u32);
    }
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: FontOrigin::BuiltIn,
        coverage: Some(coverage),
    };
    let mut font = ParsedFont::from_locator(&handle).unwrap();
    font.is_math_font = is_math_font;
    font
}

#[test]
fn exports_color_primitives() {
    assert_eq!(linear_u8_to_srgb8(0), 0);
    assert_eq!(SrgbaPixel::rgba(1, 2, 3, 4).as_rgba(), (1, 2, 3, 4));
}

#[test]
fn fallback_selection_prefers_text_for_shared_symbols_and_keeps_math_only_coverage() {
    // Contract: math coverage cannot displace text coverage or be dropped when it alone fills a gap.
    let shared = '\u{23fa}';
    let math_only = '\u{2211}';
    let math = fallback_fixture(&[shared, math_only], true);
    let text = fallback_fixture(&[shared], false);
    let mut wanted = RangeSet::new();
    wanted.add(shared as u32);
    wanted.add(math_only as u32);

    let mut selected = vec![math, text];
    select_fallback_fonts(&mut selected, &mut wanted, true);

    assert_eq!(selected.len(), 2);
    assert!(!selected[0].is_math_font);
    assert!(selected[1].is_math_font);
    assert!(wanted.is_empty());
}

#[test]
fn gdi_font_creation_failures_are_rejected_before_use() {
    const SOURCE: &str = include_str!("locator/gdi.rs");

    assert!(SOURCE.contains("anyhow::ensure!(!font.is_null(), \"font handle is null\")"));
    assert!(SOURCE.contains("anyhow::ensure!(!hdc.is_null(), \"CreateCompatibleDC failed\")"));
    assert!(SOURCE.contains("if previous.is_null()"));
    assert!(SOURCE.contains("SelectObject(hdc, previous)"));
    assert_eq!(
        SOURCE.matches("anyhow::ensure!(!font.is_null(), \"CreateFontIndirectW failed\")").count(),
        2
    );
}
