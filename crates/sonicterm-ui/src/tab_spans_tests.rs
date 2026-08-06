use super::*;

const ACTIVE: TabSpanColor = TabSpanColor::rgb(255, 128, 0);
const INACTIVE: TabSpanColor = TabSpanColor::rgb(128, 128, 128);

#[test]
fn tab_titles_are_one_logical_pixel_larger_than_body_text() {
    assert_eq!(tab_title_font_size(13.0), 14.0);
    assert_eq!(tab_title_font_size(1.0), 2.0);
}

#[test]
fn active_padding_is_colored_while_inactive_titles_start_at_their_center() {
    let tabs = [
        TabSpanInput {
            index: 0,
            title: "ab",
            title_x: 0.0,
            title_w: 50.0,
            is_active: true,
            badge: None,
        },
        TabSpanInput {
            index: 1,
            title: "cd",
            title_x: 60.0,
            title_w: 50.0,
            is_active: false,
            badge: None,
        },
    ];

    let (text, spans) = build_tab_title_spans(&tabs, 10.0, ACTIVE, INACTIVE);

    assert_eq!(text, " ab    cd");
    assert_eq!(spans, vec![(0..5, ACTIVE), (7..9, INACTIVE)]);
    assert_eq!(&text[spans[0].0.clone()], " ab  ");
    assert_eq!(&text[spans[1].0.clone()], "cd");
}

#[test]
fn badge_text_participates_in_unicode_safe_truncation() {
    let tabs = [TabSpanInput {
        index: 0,
        title: "任务完成",
        title_x: 0.0,
        title_w: 62.0,
        is_active: false,
        badge: Some("✓"),
    }];

    let (text, spans) = build_tab_title_spans(&tabs, 10.0, ACTIVE, INACTIVE);

    assert_eq!(text, "✓ 任务…");
    assert_eq!(text.chars().count(), 5);
    assert_eq!(spans, vec![(0..text.len(), INACTIVE)]);
}

#[test]
fn a_title_narrower_than_one_glyph_collapses_to_an_ellipsis() {
    let tabs = [TabSpanInput {
        index: 0,
        title: "long title",
        title_x: 0.0,
        title_w: 5.0,
        is_active: true,
        badge: None,
    }];

    let (text, spans) = build_tab_title_spans(&tabs, 10.0, ACTIVE, INACTIVE);

    assert_eq!(text, "…");
    assert_eq!(spans, vec![(0.."…".len(), ACTIVE)]);
}

#[test]
fn rich_text_spans_fill_uncolored_prefixes_and_suffixes_with_the_fallback() {
    let accent = TabSpanColor::rgba(1, 2, 3, 4);
    let rich = build_tab_title_rich_text_spans("aaββzz", &[(2..6, accent)], "ignored", INACTIVE);

    assert_eq!(
        rich.spans,
        vec![
            ("aa", INACTIVE, TabSpanAttrs::default()),
            ("ββ", accent, TabSpanAttrs::default()),
            ("zz", INACTIVE, TabSpanAttrs::default()),
        ]
    );
    assert_eq!(rich.default_color, INACTIVE);
    assert_eq!(rich.default_attrs, TabSpanAttrs::default());
}

#[test]
fn rich_text_without_colored_ranges_preserves_the_whole_title() {
    let rich = build_tab_title_rich_text_spans("plain", &[], "ignored", INACTIVE);
    assert_eq!(rich.spans, vec![("plain", INACTIVE, TabSpanAttrs::default())]);

    let empty = build_tab_title_rich_text_spans("", &[], "ignored", INACTIVE);
    assert!(empty.spans.is_empty());
}
