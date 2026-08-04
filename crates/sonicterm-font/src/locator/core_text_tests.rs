#[test]
fn fallback_range_uses_utf16_code_units() {
    assert_eq!('A'.len_utf16(), 1);
    assert_eq!('😀'.len_utf16(), 2);

    let source = include_str!("core_text.rs");
    assert!(source.contains("let utf16_len = c.len_utf16() as isize;"));
    assert!(source.contains("CFRange::init(0, utf16_len)"));
}

#[test]
fn core_foundation_ownership_rules_match_api_families() {
    let source = include_str!("core_text.rs");
    assert!(source.contains("CFArray::wrap_under_create_rule(array)"));
    assert!(source.contains("CFArray::wrap_under_get_rule(languages)"));
}
