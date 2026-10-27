//! Behavior contracts for text status presentation.

use super::*;

#[test]
fn byte_formatting_uses_three_significant_figures() {
    // Binary units, three significant figures, rollover at 1024 and the locale's decimal separator.
    let point = NumberStyle::POINT;
    assert_eq!(format_bytes(512, point), "512 B");
    assert_eq!(format_bytes(412 * MIB, point), "412 MB");
    assert_eq!(format_bytes(1023 * MIB, point), "1023 MB");
    assert_eq!(format_bytes(1024 * MIB - 1024 * 300, point), "1.00 GB");
    assert_eq!(format_bytes(1024 * MIB, point), "1.00 GB");
    assert_eq!(format_bytes(1045 * MIB, point), "1.02 GB");
    assert_eq!(format_bytes(15 * MIB + MIB / 2, point), "15.5 MB");
    assert_eq!(format_bytes(1045 * MIB, NumberStyle::for_locale("de-DE")), "1,02 GB");
}

#[test]
fn counts_group_per_locale_and_durations_never_localize() {
    // Digit grouping follows the locale; duration suffixes are fixed in every locale.
    assert_eq!(format_count(12_345, NumberStyle::for_locale("en")), "12,345");
    assert_eq!(format_count(12_345, NumberStyle::for_locale("zh-CN")), "12,345");
    assert_eq!(format_count(12_345, NumberStyle::for_locale("de")), "12.345");
    assert_eq!(format_count(12_345, NumberStyle::for_locale("fr-FR")), "12\u{202F}345");
    assert_eq!(format_count(999, NumberStyle::POINT), "999");
    assert_eq!(format_count(1_000_000, NumberStyle::POINT), "1,000,000");
    assert_eq!(
        [format_duration(42), format_duration(59), format_duration(60)],
        ["42s", "59s", "1m00s"]
    );
    assert_eq!(
        [format_duration(134), format_duration(3600), format_duration(3900)],
        ["2m14s", "1h00m", "1h05m"]
    );
}

#[test]
fn sanitizer_removes_controls_and_keeps_text() {
    // Controls, bidi and format characters go; ZWJ emoji, CJK and combining marks stay; caps hold.
    let limits = SanitizeLimits { max_bytes: 1024, max_columns: 200, collapse_whitespace: false };
    let hostile =
        "a\u{7}b\u{9B}c\u{61C}d\u{200E}e\u{202E}f\u{2066}g\u{2028}h\u{2029}i\u{FEFF}j\u{AD}k";
    assert_eq!(sanitize_display_text(hostile, SanitizePolicy::Ordinary, limits), "abcdefghijk");
    let family = "👨\u{200D}👩\u{200D}👧";
    assert_eq!(sanitize_display_text(family, SanitizePolicy::Ordinary, limits), family);
    assert_eq!(
        sanitize_display_text("漢字 e\u{301}", SanitizePolicy::Ordinary, limits),
        "漢字 e\u{301}"
    );
    assert_eq!(
        sanitize_display_text("a│b|c⟨d⟩e<f>g", SanitizePolicy::Ordinary, limits),
        "a│b|c⟨d⟩e<f>g"
    );
    assert_eq!(
        sanitize_display_text("a│b|c⟨d⟩e<f>g", SanitizePolicy::Program, limits),
        "a b c d e f g"
    );
    let big = "x".repeat(16 * 1024);
    let capped = sanitize_display_text(
        &big,
        SanitizePolicy::Ordinary,
        SanitizeLimits { max_bytes: 4096, max_columns: 10_000, collapse_whitespace: false },
    );
    assert_eq!(capped.len(), 4096);
    let collapsed = SanitizeLimits { collapse_whitespace: true, ..limits };
    assert_eq!(sanitize_display_text("  a \t\n b  ", SanitizePolicy::Ordinary, collapsed), "a b");
    let wide = sanitize_display_text(
        "漢字漢字漢字漢字",
        SanitizePolicy::Ordinary,
        SanitizeLimits { max_columns: 7, ..limits },
    );
    assert_eq!(wide, "漢…漢字");
    assert!(wide.width() <= 7);
    let byte_cut = sanitize_display_text(
        "ééé",
        SanitizePolicy::Ordinary,
        SanitizeLimits { max_bytes: 5, ..limits },
    );
    assert_eq!(byte_cut, "éé");
}

#[test]
fn ellipsize_middle_bytes_keeps_both_ends_on_grapheme_boundaries() {
    // A notification part over its byte cap keeps its start and end around one ellipsis,
    // never splitting a grapheme, and never exceeds the cap.
    assert_eq!(ellipsize_middle_bytes("short", 10), "short");
    let cut = ellipsize_middle_bytes("abcdefghijklmnop", 9);
    assert!(cut.len() <= 9 && cut.starts_with("ab") && cut.ends_with('p'), "{cut}");
    let flags = "🇯🇵".repeat(10);
    let cut = ellipsize_middle_bytes(&flags, 20);
    assert!(cut.len() <= 20);
    assert!(cut.replace('…', "").chars().count().is_multiple_of(2), "no flag is split: {cut}");
    assert_eq!(ellipsize_middle_bytes("abcdef", 2), "", "no room for the ellipsis");
}
