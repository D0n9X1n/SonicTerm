//! Behavior contracts for links status presentation.

use super::*;
use crate::i18n::test_translator as translator;
use crate::status_line::status_line_tests::*;

#[test]
fn link_forms_cover_mismatch_paths_and_failures() {
    // Each detection outcome reads as one link value; the mismatch warns with both hosts.
    let format = |link: LinkInput| {
        let inputs = StatusLineInputs { link: Some(link), ..Default::default() };
        let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
        shown(&view, StatusSegmentId::Link).unwrap()
    };
    assert_eq!(
        format(LinkInput::Mismatch {
            target_host: "evil.example".into(),
            label_host: "bank.example".into()
        }),
        "Link: ⚠ evil.example ≠ bank.example"
    );
    assert_eq!(
        format(LinkInput::Path { path: "~/proj/src".into(), action: PathAction::Reveal }),
        "Path: ~/proj/src · reveal"
    );
    assert_eq!(format(LinkInput::Checking { path: "~/a".into() }), "Path: ~/a · checking");
    assert_eq!(format(LinkInput::NotFound { path: "~/gone".into() }), "Path: ✗ ~/gone · not found");
    assert_eq!(
        format(LinkInput::Blocked {
            path: "/x".into(),
            reason: "no shell-reported directory".into()
        }),
        "Path: ⚠ /x · blocked: no shell-reported directory"
    );
    assert_eq!(
        LinkInput::from_url("mailto:me"),
        LinkInput::Url { host: "mailto:me".into(), rest: String::new() }
    );
    assert_eq!(
        LinkInput::from_url("https://host\u{202E}.example/a\u{7}b"),
        LinkInput::Url { host: "host.example".into(), rest: "/ab".into() }
    );
}

#[test]
fn hide_sensitive_removes_the_link_and_its_bounds() {
    // The link is whole-sensitive: hidden, it has no text, slot or click.
    let inputs = StatusLineInputs {
        link: Some(LinkInput::from_url("https://a.example/b")),
        ..idle_inputs()
    };
    let shown_view =
        build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    assert!(shown(&shown_view, StatusSegmentId::Link).is_some());
    let hidden = build_view(&developer(), true, &inputs, &translator("en"), NumberStyle::POINT);
    assert!(shown(&hidden, StatusSegmentId::Link).is_none());
    assert_eq!(hidden.segments.len(), 1);
}

/// The heuristic label check: a different host or any userinfo mismatches, ports and case are
/// ignored, a word is plain, and a label that was not read whole is never a match.
#[test]
fn link_label_check_follows_the_heuristic() {
    use LinkLabelCheck::{Matches, Mismatch, NotChecked, Plain};
    let mismatch = |host: &str| Mismatch { label_host: host.into() };
    let cases = [
        ("https://bank.example", true, "https://evil.example", mismatch("bank.example")),
        ("bank.example@evil.example", true, "https://evil.example/", mismatch("bank.example")),
        ("Login", true, "https://bank.example@evil.example/", mismatch("bank.example")),
        ("docs", true, "https://example.com/docs", Plain),
        ("v1.2", true, "https://example.com/", Plain),
        ("example.com/a", true, "https://example.com/a/", Matches),
        ("https://example.com:443", true, "https://example.com", Matches),
        ("HTTPS://EXAMPLE.com./x", true, "https://example.com", Matches),
        ("https://example.com", false, "https://example.com", NotChecked),
        ("bank.example", true, "mailto:me@bank.example", Plain),
    ];
    for (label, complete, target, expected) in cases {
        assert_eq!(check_link_label(label, complete, target), expected, "{label} -> {target}");
    }
}

/// A hovered OSC 8 link reads as a mismatch warning, an unchecked link, or the plain URL.
#[test]
fn hovered_links_show_mismatch_or_label_not_checked() {
    let shown = |link: LinkInput| {
        let inputs = StatusLineInputs { link: Some(link), ..Default::default() };
        let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
        shown(&view, StatusSegmentId::Link).unwrap()
    };
    assert_eq!(
        shown(LinkInput::from_hover("https://evil.example/login", Some(("bank.example", true)))),
        "Link: ⚠ evil.example ≠ bank.example"
    );
    assert_eq!(
        shown(LinkInput::from_hover("https://example.com/a", Some(("https://exam", false)))),
        "Link: example.com/a · label not checked"
    );
    assert_eq!(
        shown(LinkInput::from_hover("https://example.com/a", Some(("Docs", true)))),
        "Link: example.com/a"
    );
    assert_eq!(shown(LinkInput::from_hover("https://example.com/a", None)), "Link: example.com/a");
}

/// Right-to-left path text: bidi controls are removed by the ordinary sanitizer, Arabic and Hebrew
/// letters and combining marks are kept, and the middle ellipsis cuts on grapheme boundaries.
#[test]
fn rtl_path_text_is_sanitized_and_ellipsized_by_grapheme() {
    let limits = SanitizeLimits { max_bytes: 4096, max_columns: 200, collapse_whitespace: false };
    let arabic = "~/مشروع/\u{202E}ملفات\u{202C}/تقرير\u{2067}.txt\u{2069}";
    let clean = sanitize_display_text(arabic, SanitizePolicy::Ordinary, limits);
    assert_eq!(clean, "~/مشروع/ملفات/تقرير.txt");
    assert!(!clean
        .chars()
        .any(|character| matches!(u32::from(character), 0x202A..=0x202E | 0x2066..=0x2069)));
    let hebrew = "/בית/\u{200F}מסמכים/דּוּחַ\u{200E}.md";
    assert_eq!(
        sanitize_display_text(hebrew, SanitizePolicy::Ordinary, limits),
        "/בית/מסמכים/דּוּחַ.md"
    );
    for text in [clean.as_str(), "/בית/מסמכים/דּוּחַ.md"] {
        let mut measure = columns;
        let cut = ellipsize_middle_to_width(text, 10.0, &mut measure);
        assert!(cut.contains('…') && cut.width() <= 10, "{cut}");
        let whole: Vec<&str> = text.graphemes(true).collect();
        assert!(
            cut.graphemes(true).all(|grapheme| grapheme == "…" || whole.contains(&grapheme)),
            "{cut}"
        );
        let column_cut = ellipsize_middle_columns(text, 9);
        assert!(
            column_cut.width() <= 9
                && column_cut
                    .graphemes(true)
                    .all(|grapheme| grapheme == "…" || whole.contains(&grapheme))
        );
    }
}
