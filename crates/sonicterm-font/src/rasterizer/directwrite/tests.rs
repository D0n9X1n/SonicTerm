use super::*;

#[test]
fn directwrite_coverage_enhancement_preserves_extremes() {
    assert_eq!(enhance_text_coverage(0), 0);
    assert_eq!(enhance_text_coverage(255), 255);
}

#[test]
fn directwrite_coverage_enhancement_darkens_midtones() {
    let mid = enhance_text_coverage(128);
    assert!(mid > 128, "midtone coverage should be stronger, got {mid}");
    assert!(mid < 160, "coverage boost should stay modest, got {mid}");
}

#[test]
fn directwrite_coverage_enhancement_is_monotonic_and_never_thins_text() {
    let mut prev = enhance_text_coverage(0);
    for coverage in 1..=u8::MAX {
        let enhanced = enhance_text_coverage(coverage);
        assert!(
            enhanced >= coverage,
            "coverage boost must not make glyphs thinner: {coverage} -> {enhanced}"
        );
        assert!(
            enhanced >= prev,
            "coverage boost must remain monotonic: prev {prev}, current {enhanced}"
        );
        prev = enhanced;
    }
}
