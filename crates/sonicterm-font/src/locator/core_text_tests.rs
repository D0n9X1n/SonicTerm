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

/// A simplified-only character gets a fallback face that covers it, whether CoreText's own choice,
/// PingFang, opens through HVF (macOS 15.4+) or fails and leaves it to the generic fallback list.
#[test]
fn simplified_chinese_has_a_fallback_face() {
    use crate::locator::FontLocator;
    let mut wanted = crate::rangeset::RangeSet::new();
    wanted.add('测' as u32);
    let fonts = super::CoreTextFontLocator {}
        .locate_fallback_for_codepoints(&['测'])
        .expect("CoreText fallback");
    let covering = fonts
        .iter()
        .find(|font| font.coverage_intersection(&wanted).is_ok_and(|cov| !cov.is_empty()));
    assert!(covering.is_some(), "no fallback face covers 测: {fonts:?}");
}

/// The generic fallback list parses each font file once: CoreText's cascade names a collection
/// once per face it lists (PingFang eight times), the named families can name a file the cascade
/// already listed, and every parse opens all of the file's faces.
#[test]
fn the_fallback_list_parses_each_cascade_file_once() {
    let menlo = super::new_from_name("Menlo", 0.0).expect("Menlo");
    let languages = super::objc_compat::apple_languages().expect("languages");
    let cascade: Vec<_> = super::cascade_list_for_languages(&menlo, &languages)
        .iter()
        .filter_map(|descriptor| descriptor.font_path())
        .collect();
    let distinct: std::collections::HashSet<_> = cascade.iter().collect();
    assert!(cascade.len() > distinct.len(), "the cascade repeats a file, so the test can tell");
    super::FILES_PARSED.with(|parsed| parsed.borrow_mut().clear());
    let _ = super::build_fallback_list();
    let parsed = super::FILES_PARSED.with(|parsed| parsed.take());
    let mut seen = std::collections::HashSet::new();
    for file in &parsed {
        assert!(seen.insert(file), "{} parsed more than once", file.display());
    }
}
