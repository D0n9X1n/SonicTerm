#[test]
fn fallback_range_uses_utf16_code_units() {
    assert_eq!('A'.len_utf16(), 1);
    assert_eq!('😀'.len_utf16(), 2);

    let source = include_str!("core_text.rs");
    assert!(source.contains("let utf16_len = character.len_utf16() as isize;"));
    assert!(source.contains("CFRange::init(0, utf16_len)"));
}

#[test]
fn core_foundation_ownership_rules_match_api_families() {
    let source = include_str!("core_text.rs");
    assert!(source.contains("CFArray::wrap_under_create_rule(array)"));
    assert!(source.contains("CFArray::wrap_under_get_rule(languages)"));
}

/// One fallback request computes each candidate face's coverage once, not once per codepoint that
/// the face covers, so a run of Han characters costs one cmap walk per face.
///
/// A request first warms the process-wide generic fallback list: a host whose CoreText face for
/// Han text cannot be parsed falls through to that list, and the first request to reach it would
/// otherwise count the list's one-time coverage as its own.
#[test]
fn fallback_computes_each_face_coverage_once_per_request() {
    use crate::locator::FontLocator;
    super::CoreTextFontLocator {}
        .locate_fallback_for_codepoints(&['漢', '字'])
        .expect("CoreText fallback");
    let computed = |codepoints: &[char]| {
        crate::parser::COVERAGE_COMPUTED.with(|count| count.set(0));
        let fonts = super::CoreTextFontLocator {}
            .locate_fallback_for_codepoints(codepoints)
            .expect("CoreText fallback");
        assert!(!fonts.is_empty(), "{codepoints:?} has a fallback face");
        crate::parser::COVERAGE_COMPUTED.with(std::cell::Cell::get)
    };
    let pair = computed(&['漢', '字']);
    let many = computed(&['漢', '字', '中', '文', '東', '京', '語', '本']);
    assert_eq!(many, pair, "coverage is computed per face, not per codepoint");
}
