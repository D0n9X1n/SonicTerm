use super::*;

/// The Rec Mono fixture's regular face, read the way the renderer's font files are.
fn fixture_font() -> Vec<u8> {
    let fonts =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts");
    std::fs::read(fonts.join("rec-mono/RecMonoSt.Helens-Regular.ttf"))
        .expect("the Rec Mono fixture")
}

/// A digit's extent comes from the face's own tables: fontTools reads the fixture's 2 as 0..720 and
/// its 7 as -10..720 in 1000 units. A codepoint the face cannot map has no extent.
#[test]
fn a_glyph_extent_is_the_outline_y_range() {
    let font = fixture_font();
    let extent = |y_min, y_max| Some(GlyphExtent { units_per_em: 1000, y_min, y_max });
    assert_eq!(glyph_extent(&font, '2'), extent(0, 720));
    assert_eq!(glyph_extent(&font, '7'), extent(-10, 720));
    assert_eq!(glyph_extent(&font, '\u{F0000}'), None, "outside a format 4 subtable");
    assert_eq!(glyph_extent(b"ttcf", '2'), None, "a collection header is no face");
}

/// The threshold is the least coverage whose blend of the default foreground (#cfbc97) over the
/// default background (#141617) moves a channel more than 80 levels: 0.197, as an independent sRGB
/// model of the same linear-light blend gives. A colour over itself never shows.
#[test]
fn the_ink_threshold_is_where_the_blend_first_shows() {
    let threshold = ink_threshold([0xcf, 0xbc, 0x97], [0x14, 0x16, 0x17]);
    assert!((threshold - 0.1974).abs() < 0.002, "{threshold}");
    let unseen = ink_threshold([0x14, 0x16, 0x17], [0x14, 0x16, 0x17]);
    assert!((unseen - 1.0).abs() < f32::EPSILON, "{unseen}");
}

/// A partial top row counts only when the blend shows it. Cascadia Mono's round 2 reaches 1440 of
/// 2048 units and its flat 7 1420: at 21.75 px (14.5 pt at 1.5x) they cover 0.29 and 0.08 of their
/// top rows, so 2 shows 16 rows and 7 15. At 18.125 and 25.375 px both partial rows show.
#[test]
fn a_partial_top_row_counts_only_when_the_blend_shows_it() {
    let extent = |y_max: i16| GlyphExtent { units_per_em: 2048, y_min: 0, y_max };
    let rows = |y_max: i16, strike_px: f32| {
        predicted_ink_top(extent(y_max), strike_px, 0.1974).rows_above_baseline
    };
    assert_eq!((rows(1440, 21.75), rows(1420, 21.75)), (16, 15));
    assert_eq!((rows(1440, 18.125), rows(1420, 18.125)), (13, 13));
    assert_eq!((rows(1440, 25.375), rows(1420, 25.375)), (18, 18));
    let top = predicted_ink_top(extent(1440), 21.75, 0.1974);
    assert!((top.top_row_coverage - 0.293).abs() < 0.001, "{top:?}");
}
